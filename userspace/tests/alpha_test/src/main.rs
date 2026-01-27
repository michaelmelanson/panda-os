#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::graphics::{Colour, PixelBuffer, Window};

libpanda::main! {
    environment::log("Alpha blending test starting");

    let window_width = 350u32;
    let window_height = 250u32;

    // Create three overlapping windows with semi-transparent colours

    // Window 1: Red with 60% alpha at (50, 50)
    let mut window1 = match Window::builder()
        .size(window_width, window_height)
        .position(50, 50)
        .visible(true)
        .build()
    {
        Ok(w) => w,
        Err(_) => {
            environment::log("FAIL: Could not open window 1");
            return 1;
        }
    };
    environment::log("PASS: Opened window 1 (red)");

    let mut buffer1 = match PixelBuffer::new(window_width, window_height) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate buffer 1");
            return 1;
        }
    };

    // Fill with semi-transparent red (60% alpha = 153)
    buffer1.clear(Colour::rgba(255, 0, 0, 153));

    let _ = window1.blit(&buffer1, 0, 0);
    let _ = window1.flush();
    environment::log("PASS: Created red window with 60% alpha");

    // Window 2: Green with 60% alpha at (180, 90)
    let mut window2 = match Window::builder()
        .size(window_width, window_height)
        .position(180, 90)
        .visible(true)
        .build()
    {
        Ok(w) => w,
        Err(_) => {
            environment::log("FAIL: Could not open window 2");
            return 1;
        }
    };
    environment::log("PASS: Opened window 2 (green)");

    let mut buffer2 = match PixelBuffer::new(window_width, window_height) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate buffer 2");
            return 1;
        }
    };

    // Fill with semi-transparent green (60% alpha = 153)
    buffer2.clear(Colour::rgba(0, 255, 0, 153));

    let _ = window2.blit(&buffer2, 0, 0);
    let _ = window2.flush();
    environment::log("PASS: Created green window with 60% alpha");

    // Window 3: Blue with 60% alpha at (115, 170)
    let mut window3 = match Window::builder()
        .size(window_width, window_height)
        .position(115, 170)
        .visible(true)
        .build()
    {
        Ok(w) => w,
        Err(_) => {
            environment::log("FAIL: Could not open window 3");
            return 1;
        }
    };
    environment::log("PASS: Opened window 3 (blue)");

    let mut buffer3 = match PixelBuffer::new(window_width, window_height) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate buffer 3");
            return 1;
        }
    };

    // Fill with semi-transparent blue (60% alpha = 153)
    buffer3.clear(Colour::rgba(0, 0, 255, 153));

    let _ = window3.blit(&buffer3, 0, 0);
    let _ = window3.flush();
    environment::log("PASS: Created blue window with 60% alpha");

    environment::log("PASS: Alpha blending test complete");

    // Signal that screenshot can be taken
    environment::screenshot_ready();

    // Loop forever
    loop {}
}
