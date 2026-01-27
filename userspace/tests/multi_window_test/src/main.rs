#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::graphics::{Colour, PixelBuffer, Window};

libpanda::main! {
    environment::log("Multi-window test starting");

    // Create window 1 - Red window at (50, 50), size 300x200
    let mut window1 = match Window::builder()
        .size(300, 200)
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

    // Create and fill buffer for window 1 (red)
    let mut buffer1 = match PixelBuffer::new(300, 200) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate buffer 1");
            return 1;
        }
    };

    buffer1.clear(Colour::RED);

    if window1.blit(&buffer1, 0, 0).is_err() || window1.flush().is_err() {
        environment::log("FAIL: Could not render window 1");
        return 1;
    }
    environment::log("PASS: Created and rendered red window at (50, 50)");

    // Create window 2 - Blue window at (150, 100), size 300x200 (overlaps window 1)
    let mut window2 = match Window::builder()
        .size(300, 200)
        .position(150, 100)
        .visible(true)
        .build()
    {
        Ok(w) => w,
        Err(_) => {
            environment::log("FAIL: Could not open window 2");
            return 1;
        }
    };

    // Create and fill buffer for window 2 (blue)
    let mut buffer2 = match PixelBuffer::new(300, 200) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate buffer 2");
            return 1;
        }
    };

    buffer2.clear(Colour::BLUE);

    if window2.blit(&buffer2, 0, 0).is_err() || window2.flush().is_err() {
        environment::log("FAIL: Could not render window 2");
        return 1;
    }
    environment::log("PASS: Created and rendered blue window at (150, 100)");

    environment::log("PASS: Multi-window test complete - blue should overlap red");

    // Signal screenshot ready
    environment::screenshot_ready();

    // Loop forever
    loop {}
}
