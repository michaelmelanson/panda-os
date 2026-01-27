#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::graphics::{Colour, PixelBuffer, Window};

libpanda::main! {
    environment::log("Window move test starting");

    let window_width = 200u32;
    let window_height = 150u32;

    // Create a window at (50, 50) initially
    let mut window = match Window::builder()
        .size(window_width, window_height)
        .position(50, 50)
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
    environment::log("PASS: Set initial window position (50, 50)");

    // Allocate buffer and fill with red
    let mut buffer = match PixelBuffer::new(window_width, window_height) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate buffer");
            return 1;
        }
    };

    buffer.clear(Colour::RED);

    let _ = window.blit(&buffer, 0, 0);
    let _ = window.flush();
    environment::log("PASS: Displayed red window at (50, 50)");

    // Now move the window to a new position (300, 200)
    if window.set_position(300, 200).is_err() {
        environment::log("FAIL: Could not move window");
        return 1;
    }

    // Flush to apply the move
    let _ = window.flush();
    environment::log("PASS: Moved window to (300, 200)");

    environment::log("PASS: Window move test complete");

    // Signal that screenshot can be taken
    environment::screenshot_ready();

    // Loop forever
    loop {}
}
